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

use std::{collections::BTreeMap, marker::PhantomData};

use bevy::prelude::*;
use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

use super::{declarative_prior::GenerationPriorSpec, decode};
use crate::capability_registry::{
    AssemblyPatternDescriptor, AssemblyTypeDescriptor, CapabilityRegistry, ElementClassDescriptor,
    RelationTypeDescriptor,
};
use crate::curation::{CuratedManifest, CuratedManifestRegistry, CURATED_MANIFEST_ASSET_KIND};
use crate::plugins::constraint_rules::{
    install_rule, remove_rule, ConstraintRule, DeclarativeConstraintRules,
};
use crate::plugins::materials::{
    ensure_builtin_materials, BuiltinMaterialDefaults, MaterialDef, MaterialRegistry,
};
use crate::plugins::refinement::{SettingOutThicknessPrior, SettingOutThicknessPriorRegistry};
use crate::semantics::{ConceptGraphPack, SemanticGraph};

/// A compiled registry entry offered for migration to the corpus.
pub struct Legacy<P> {
    pub id: String,
    pub label: String,
    /// The exact payload, or why this entry cannot travel as data.
    pub payload: Result<P, &'static str>,
}

/// A portable kind backed by a runtime registry.
pub trait RegistryKind: 'static {
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
pub trait CapabilityEntry: 'static {
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
pub struct InCapabilityRegistry<E>(PhantomData<E>);

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
pub struct LegacyItem {
    pub(super) id: String,
    pub(super) label: String,
    /// `None` when the entry is a portable migration candidate.
    pub(super) gap: Option<&'static str>,
}

/// Type-erased row of the kind table.
#[derive(Clone, Copy, Debug)]
pub struct KindHandler {
    pub(super) kind: &'static str,
    pub(super) constraints: &'static [&'static str],
    pub(super) payload_schema: fn() -> Value,
    /// Decode + validate `payload` against `asset_id`, withdraw `previous` (the
    /// payload of the revision being replaced), then install. Nothing is
    /// touched when decoding or validation fails.
    pub(super) install: fn(&mut World, &str, &Value, Option<&Value>) -> Result<(), String>,
    pub(super) withdraw: fn(&mut World, &Value),
    pub(super) legacy_list: fn(&World) -> Vec<LegacyItem>,
    pub(super) legacy_get: fn(&World, &str) -> Result<Value, String>,
}

impl KindHandler {
    /// The type-erased table row for `K`.
    #[must_use]
    pub const fn of<K: RegistryKind>() -> Self {
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
pub const HANDLERS: &[KindHandler] = &[
    KindHandler::of::<InCapabilityRegistry<ElementClass>>(),
    KindHandler::of::<RetainedGenerationPrior>(),
    KindHandler::of::<InCapabilityRegistry<AssemblyType>>(),
    KindHandler::of::<InCapabilityRegistry<RelationType>>(),
    KindHandler::of::<InCapabilityRegistry<AssemblyPattern>>(),
    KindHandler::of::<ConceptGraph>(),
    KindHandler::of::<Material>(),
    KindHandler::of::<ConstraintRuleKind>(),
    KindHandler::of::<SettingOutPrior>(),
    KindHandler::of::<CuratedManifestKind>(),
];

/// Every portable registry kind this process can route.
///
/// Seeded from the core [`HANDLERS`] table; domain plugins add the kinds whose
/// registries core cannot name with
/// [`KnowledgeKindAppExt::register_knowledge_kind`]. Schema, candidate check,
/// install, withdrawal and legacy inventory all resolve kinds through this
/// resource, so a registered kind is routed everywhere with no further wiring.
/// Registering a kind id that already exists replaces its handler.
#[derive(Resource, Clone, Debug)]
pub struct KnowledgeKindRegistry {
    handlers: Vec<KindHandler>,
}

impl Default for KnowledgeKindRegistry {
    fn default() -> Self {
        Self {
            handlers: HANDLERS.to_vec(),
        }
    }
}

impl KnowledgeKindRegistry {
    /// Add `handler`, replacing any handler for the same kind id.
    pub fn register(&mut self, handler: KindHandler) {
        match self.handlers.iter_mut().find(|h| h.kind == handler.kind) {
            Some(existing) => *existing = handler,
            None => self.handlers.push(handler),
        }
    }

    /// The handler for `kind`, if routed.
    #[must_use]
    pub fn get(&self, kind: &str) -> Option<KindHandler> {
        self.handlers.iter().find(|h| h.kind == kind).copied()
    }

    /// All routed handlers in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &KindHandler> {
        self.handlers.iter()
    }
}

/// `App` extension for plugins that own a portable registry.
pub trait KnowledgeKindAppExt {
    /// Route kind `K` (schema, check, install, withdrawal, legacy inventory).
    fn register_knowledge_kind<K: RegistryKind>(&mut self) -> &mut Self;
}

impl KnowledgeKindAppExt for App {
    fn register_knowledge_kind<K: RegistryKind>(&mut self) -> &mut Self {
        self.init_resource::<KnowledgeKindRegistry>();
        self.world_mut()
            .resource_mut::<KnowledgeKindRegistry>()
            .register(KindHandler::of::<K>());
        self
    }
}

/// The routing table for `world`: its resource, or the core table when absent.
fn table(world: &World) -> KnowledgeKindRegistry {
    world
        .get_resource::<KnowledgeKindRegistry>()
        .cloned()
        .unwrap_or_default()
}

/// The handler for a portable registry kind, if `kind` is routed in `world`.
pub(super) fn handler(world: &World, kind: &str) -> Option<KindHandler> {
    match world.get_resource::<KnowledgeKindRegistry>() {
        Some(registry) => registry.get(kind),
        None => HANDLERS.iter().find(|h| h.kind == kind).copied(),
    }
}

/// Every routed handler in `world`.
pub(super) fn handlers(world: &World) -> Vec<KindHandler> {
    table(world).handlers
}

/// Compiled-registry categories that cannot be delivered as data.
pub(super) const UNSUPPORTED_NATIVE: [&str; 4] = [
    "native_recipe_bodies",
    "native_generation_prior_closures",
    "native_constraints",
    "textured_materials",
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
        pub struct $name;
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
pub struct GenerationPrior;
impl CapabilityEntry for GenerationPrior {
    type Payload = GenerationPriorSpec;
    const KIND: &'static str = "generation_prior.v1";
    const CONSTRAINTS: &'static [&'static str] = &[
        "payload.id must equal asset.id",
        "A prior is a first-match-wins decision table over jurisdiction, style_intent, terrain slope and exact context keys; every weight is within [0, 1]",
        "Compiled priors registered as declarative tables are listed as portable candidates; compiled priors holding a native closure are listed as gaps with their reason",
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

/// Portable specs of compiled table priors, and why native priors are not.
///
/// Compiled plugins register priors through [`DeclaredPriorAppExt`] so the
/// legacy inventory can offer each table prior as an exact `generation_prior.v1`
/// payload, and each native prior as a gap with its own reason.
#[derive(Resource, Debug, Clone, Default)]
pub struct DeclaredPriors {
    specs: BTreeMap<String, GenerationPriorSpec>,
    native_gaps: BTreeMap<String, &'static str>,
}

/// `App` extension for compiled plugins that register generation priors.
pub trait DeclaredPriorAppExt {
    /// Register a declarative prior and retain its portable spec.
    fn register_declarative_generation_prior(&mut self, spec: GenerationPriorSpec) -> &mut Self;
    /// Register a native prior and record why it cannot travel as a table.
    fn register_native_generation_prior(
        &mut self,
        descriptor: crate::capability_registry::GenerationPriorDescriptor,
        gap: &'static str,
    ) -> &mut Self;
}

impl DeclaredPriorAppExt for App {
    fn register_declarative_generation_prior(&mut self, spec: GenerationPriorSpec) -> &mut Self {
        self.init_resource::<DeclaredPriors>();
        self.world_mut()
            .resource_mut::<DeclaredPriors>()
            .specs
            .insert(spec.id.0.clone(), spec.clone());
        crate::capability_registry::CapabilityRegistryAppExt::register_generation_prior(
            self,
            spec.into_descriptor(),
        )
    }

    fn register_native_generation_prior(
        &mut self,
        descriptor: crate::capability_registry::GenerationPriorDescriptor,
        gap: &'static str,
    ) -> &mut Self {
        self.init_resource::<DeclaredPriors>();
        self.world_mut()
            .resource_mut::<DeclaredPriors>()
            .native_gaps
            .insert(descriptor.id.0.clone(), gap);
        crate::capability_registry::CapabilityRegistryAppExt::register_generation_prior(
            self, descriptor,
        )
    }
}

/// `generation_prior.v1`: the registry kind, with compiled table priors
/// offered as exact payloads through [`DeclaredPriors`].
pub struct RetainedGenerationPrior;
type CorePrior = InCapabilityRegistry<GenerationPrior>;
impl RegistryKind for RetainedGenerationPrior {
    type Payload = GenerationPriorSpec;
    const KIND: &'static str = <CorePrior as RegistryKind>::KIND;
    const CONSTRAINTS: &'static [&'static str] = <CorePrior as RegistryKind>::CONSTRAINTS;
    fn payload_id(payload: &Self::Payload) -> &str {
        <CorePrior as RegistryKind>::payload_id(payload)
    }
    fn validate(payload: &Self::Payload) -> Result<(), String> {
        <CorePrior as RegistryKind>::validate(payload)
    }
    fn install(world: &mut World, payload: Self::Payload) {
        world.init_resource::<DeclaredPriors>();
        let mut declared = world.resource_mut::<DeclaredPriors>();
        declared.native_gaps.remove(&payload.id.0);
        declared.specs.insert(payload.id.0.clone(), payload.clone());
        <CorePrior as RegistryKind>::install(world, payload);
    }
    fn withdraw(world: &mut World, payload: &Self::Payload) {
        if let Some(mut declared) = world.get_resource_mut::<DeclaredPriors>() {
            declared.specs.remove(&payload.id.0);
        }
        <CorePrior as RegistryKind>::withdraw(world, payload);
    }
    fn legacy(world: &World) -> Vec<Legacy<Self::Payload>> {
        let declared = world.get_resource::<DeclaredPriors>();
        <CorePrior as RegistryKind>::legacy(world)
            .into_iter()
            .map(|mut row| {
                if let Some(declared) = declared {
                    if let Some(spec) = declared.specs.get(&row.id) {
                        row.payload = Ok(spec.clone());
                    } else if let Some(gap) = declared.native_gaps.get(&row.id) {
                        row.payload = Err(gap);
                    }
                }
                row
            })
            .collect()
    }
}

/// Design Concept Graph records, delivered as a pack under one identity.
///
/// A pack, not one record per asset: publications, propositions and anchor kinds
/// reference one another and feed compiled indexes, so they are installed,
/// replaced and withdrawn atomically.
pub struct ConceptGraph;
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

const TEXTURE_GAP: &str = "Material references a texture (file, bundle or bytes) that cannot travel as portable data; only parameter/procedural materials are portable";

/// Parameter/procedural material definitions.
///
/// Installed into the retained `BuiltinMaterialDefaults` (so the material
/// survives document replacement exactly like compiled presets) and into the
/// live `MaterialRegistry`.
pub struct Material;
impl RegistryKind for Material {
    type Payload = MaterialDef;
    const KIND: &'static str = "material.v1";
    const CONSTRAINTS: &'static [&'static str] = &[
        "payload.id must equal asset.id",
        "Only parameter/procedural materials are portable: every texture slot (base_color, normal_map, metallic_roughness, emissive, occlusion) must be null; file, bundle and embedded-bytes textures cannot be delivered",
        "Scalar PBR parameters (roughness, metallic, reflectance, transmission, clearcoat, alpha_cutoff) are within [0, 1]; ior is at least 1; all values are finite",
        "Installing replaces a compiled or earlier material with the same id; withdrawal removes it and restores nothing",
    ];
    fn payload_id(payload: &Self::Payload) -> &str {
        &payload.id
    }
    fn validate(payload: &Self::Payload) -> Result<(), String> {
        payload.validate_portable()
    }
    fn install(world: &mut World, payload: Self::Payload) {
        if let Some(mut registry) = world.get_resource_mut::<MaterialRegistry>() {
            registry.upsert(payload.clone());
        }
        world.init_resource::<BuiltinMaterialDefaults>();
        world
            .resource_mut::<BuiltinMaterialDefaults>()
            .insert(payload);
    }
    fn withdraw(world: &mut World, payload: &Self::Payload) {
        if let Some(mut defaults) = world.get_resource_mut::<BuiltinMaterialDefaults>() {
            defaults.remove(&payload.id);
        }
        if let Some(mut registry) = world.get_resource_mut::<MaterialRegistry>() {
            registry.remove(&payload.id);
        }
    }
    fn legacy(world: &World) -> Vec<Legacy<Self::Payload>> {
        let mut compiled = MaterialRegistry::default();
        ensure_builtin_materials(&mut compiled);
        let mut by_id: BTreeMap<String, MaterialDef> =
            compiled.all().map(|m| (m.id.clone(), m.clone())).collect();
        if let Some(defaults) = world.get_resource::<BuiltinMaterialDefaults>() {
            by_id.extend(defaults.iter().map(|m| (m.id.clone(), m.clone())));
        }
        rows(
            by_id.into_values(),
            |m| m.id.as_str(),
            |m| m.name.as_str(),
            |m| (!m.has_textures()).then_some(m).ok_or(TEXTURE_GAP),
        )
    }
}

/// Declarative validation rules, evaluated by the one generic validator in
/// `constraint_rules` through the ordinary constraint registry.
pub struct ConstraintRuleKind;
impl RegistryKind for ConstraintRuleKind {
    type Payload = ConstraintRule;
    const KIND: &'static str = "constraint_rule.v1";
    const CONSTRAINTS: &'static [&'static str] = &[
        "payload.id must equal asset.id and becomes the constraint id",
        "A rule applies to one element_class from refinement state from_state upward; the requirement (lt, lte, gt, gte, eq with tolerance, range) states what must hold and a finding is emitted when it does not",
        "parameter_path is a JSON pointer into the subject's SemanticIntent.parameters; values and comparison limits are in unit; a missing or non-numeric value emits nothing",
        "jurisdiction, when set, restricts the rule to subjects whose SemanticIntent.parameters.jurisdiction equals it (case-insensitive)",
        "citation names the corpus passage that establishes the requirement and is attached to every finding as its backlink",
        "Installing replaces a compiled or earlier constraint with the same id; withdrawal removes it and restores nothing",
        "Compiled validators hold native closures and are listed in the legacy inventory as gaps, not exportable payloads",
    ];
    fn payload_id(payload: &Self::Payload) -> &str {
        &payload.id
    }
    fn validate(payload: &Self::Payload) -> Result<(), String> {
        payload.validate()
    }
    fn install(world: &mut World, payload: Self::Payload) {
        install_rule(world, payload);
    }
    fn withdraw(world: &mut World, payload: &Self::Payload) {
        remove_rule(world, &payload.id);
    }
    fn legacy(world: &World) -> Vec<Legacy<Self::Payload>> {
        let declared = world.get_resource::<DeclarativeConstraintRules>();
        let Some(registry) = world.get_resource::<CapabilityRegistry>() else {
            return Vec::new();
        };
        rows(
            registry.constraint_descriptors().iter(),
            |d| d.id.0.as_str(),
            |d| d.label.as_str(),
            |d| {
                declared
                    .and_then(|rules| rules.0.get(&d.id.0))
                    .cloned()
                    .ok_or(NATIVE_CONSTRAINT_GAP)
            },
        )
    }
}

const NATIVE_CONSTRAINT_GAP: &str = "Compiled constraint holds a native validator closure that cannot be serialised; author an equivalent declarative constraint_rule.v1 from grounded sources";

/// Setting-out thickness priors (plain evidence-backed data).
pub struct SettingOutPrior;
impl RegistryKind for SettingOutPrior {
    type Payload = SettingOutThicknessPrior;
    const KIND: &'static str = "setting_out_prior.v1";
    const CONSTRAINTS: &'static [&'static str] = &[
        "payload.prior_id must equal asset.id",
        "source_ref is required; confidence is within [0, 1]; thickness_mm is positive; negative_offset_mm + positive_offset_mm equals thickness_mm",
        "Selection prefers a prior naming the construction system and jurisdiction over a general one for the same element_class",
        "Installing replaces a compiled or earlier prior with the same id; withdrawal removes it and restores nothing",
    ];
    fn payload_id(payload: &Self::Payload) -> &str {
        &payload.prior_id
    }
    fn validate(payload: &Self::Payload) -> Result<(), String> {
        payload.validate()
    }
    fn install(world: &mut World, payload: Self::Payload) {
        world.init_resource::<SettingOutThicknessPriorRegistry>();
        if let Err(error) = world
            .resource_mut::<SettingOutThicknessPriorRegistry>()
            .register(payload)
        {
            warn!("validated setting-out prior was rejected on install: {error}");
        }
    }
    fn withdraw(world: &mut World, payload: &Self::Payload) {
        if let Some(mut registry) = world.get_resource_mut::<SettingOutThicknessPriorRegistry>() {
            registry.remove(&payload.prior_id);
        }
    }
    fn legacy(world: &World) -> Vec<Legacy<Self::Payload>> {
        world
            .get_resource::<SettingOutThicknessPriorRegistry>()
            .map(|r| {
                rows(
                    r.all().iter().cloned(),
                    |p| p.prior_id.as_str(),
                    |p| p.element_class.as_str(),
                    Ok,
                )
            })
            .unwrap_or_default()
    }
}

/// Curated manifests (for example construction-system manifests).
pub struct CuratedManifestKind;
impl RegistryKind for CuratedManifestKind {
    type Payload = CuratedManifest;
    const KIND: &'static str = "curated_manifest.v1";
    const CONSTRAINTS: &'static [&'static str] = &[
        "payload.meta.id must equal asset.id; payload.meta.kind must be curated_manifest.v1",
        "manifest_kind names a manifest kind descriptor registered by a capability (get its body_schema from the runtime); body must be a JSON object",
        "A manifest whose manifest_kind is not registered is stored but reported as an orphan by manifest validation",
        "Installing replaces a compiled or earlier manifest with the same id; withdrawal removes it and restores nothing",
    ];
    fn payload_id(payload: &Self::Payload) -> &str {
        &payload.meta.id.0
    }
    fn validate(payload: &Self::Payload) -> Result<(), String> {
        if payload.meta.kind != CuratedManifest::asset_kind() {
            return Err(format!("meta.kind must be {CURATED_MANIFEST_ASSET_KIND}"));
        }
        if payload.manifest_kind.as_str().trim().is_empty() {
            return Err("manifest_kind must be non-empty".into());
        }
        if !payload.body.is_object() {
            return Err("body must be a JSON object".into());
        }
        Ok(())
    }
    fn install(world: &mut World, payload: Self::Payload) {
        world.init_resource::<CuratedManifestRegistry>();
        world
            .resource_mut::<CuratedManifestRegistry>()
            .insert(payload);
    }
    fn withdraw(world: &mut World, payload: &Self::Payload) {
        if let Some(mut registry) = world.get_resource_mut::<CuratedManifestRegistry>() {
            registry.remove(&payload.meta.id);
        }
    }
    fn legacy(world: &World) -> Vec<Legacy<Self::Payload>> {
        world
            .get_resource::<CuratedManifestRegistry>()
            .map(|r| {
                rows(
                    r.iter().cloned(),
                    |m| m.meta.id.0.as_str(),
                    |m| m.manifest_kind.as_str(),
                    Ok,
                )
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        check_candidate, installed_revision, legacy_get, legacy_list, payload_schema, reconcile,
        AdmittedAsset, Asset, CandidateCheck, Index, LegacyRequest, SCHEMA,
    };
    use super::*;
    use crate::capability_registry::{
        CapabilityRegistry, ConstraintId, ElementClassId, PriorContext, PriorId,
    };
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
            Fixture {
                kind: "material.v1",
                id: "synthetic.material",
                compiled: serde_json::to_value(MaterialDef {
                    id: "synthetic.material".into(),
                    name: "compiled".into(),
                    ..MaterialDef::default()
                })
                .unwrap(),
                register: |w, v| {
                    w.init_resource::<BuiltinMaterialDefaults>();
                    w.resource_mut::<BuiltinMaterialDefaults>().insert(from(v));
                },
                observe: |w| {
                    w.get_resource::<BuiltinMaterialDefaults>()?
                        .iter()
                        .find(|m| m.id == "synthetic.material")
                        .map(|m| m.name.clone())
                },
                edit: |v| v["name"] = json!("corpus"),
            },
            Fixture {
                kind: "constraint_rule.v1",
                id: "synthetic.rule",
                compiled: rule_json("compiled"),
                register: |w, v| install_rule(w, from(v)),
                observe: |w| {
                    w.get_resource::<CapabilityRegistry>()?
                        .constraint_descriptor(&ConstraintId("synthetic.rule".into()))
                        .map(|d| d.label.clone())
                },
                edit: relabel,
            },
            Fixture {
                kind: "setting_out_prior.v1",
                id: "synthetic.setting_out",
                compiled: json!({"prior_id":"synthetic.setting_out","element_class":"c",
                    "construction_system_id":null,"jurisdiction":null,"thickness_mm":100.0,
                    "negative_offset_mm":50.0,"positive_offset_mm":50.0,
                    "source_ref":"compiled","confidence":0.5}),
                register: |w, v| {
                    w.init_resource::<SettingOutThicknessPriorRegistry>();
                    w.resource_mut::<SettingOutThicknessPriorRegistry>()
                        .register(from(v))
                        .unwrap();
                },
                observe: |w| {
                    w.get_resource::<SettingOutThicknessPriorRegistry>()?
                        .all()
                        .iter()
                        .find(|p| p.prior_id == "synthetic.setting_out")
                        .map(|p| p.source_ref.clone())
                },
                edit: |v| v["source_ref"] = json!("corpus"),
            },
            Fixture {
                kind: "curated_manifest.v1",
                id: "synthetic.kind/m1",
                compiled: manifest_json("compiled"),
                register: |w, v| {
                    w.init_resource::<CuratedManifestRegistry>();
                    w.resource_mut::<CuratedManifestRegistry>().insert(from(v));
                },
                observe: |w| {
                    w.get_resource::<CuratedManifestRegistry>()?
                        .get(&crate::curation::AssetId::new("synthetic.kind/m1"))
                        .and_then(|m| m.body["label"].as_str().map(str::to_owned))
                },
                edit: |v| v["body"]["label"] = json!("corpus"),
            },
        ]
    }

    fn rule_json(label: &str) -> Value {
        json!({"id":"synthetic.rule","label":label,"element_class":"synthetic.class",
            "parameter_path":"/height","requirement":{"comparator":"gte","value":2.0},
            "unit":"m","from_state":"Schematic","severity":"Warning",
            "message":"Height is below the requirement","rationale":"synthetic",
            "citation":"synthetic/passage/1"})
    }

    fn manifest_json(label: &str) -> Value {
        use crate::curation::{
            AssetId, Confidence, CurationMeta, Lineage, ManifestKindId, Provenance,
        };
        use crate::plugins::refinement::AgentId;
        let meta = CurationMeta::new(
            AssetId::new("synthetic.kind/m1"),
            CuratedManifest::asset_kind(),
            Provenance {
                author: AgentId("test".into()),
                confidence: Confidence::High,
                lineage: Lineage::Freeform,
                rationale: None,
                jurisdiction: None,
                catalog_dependencies: Vec::new(),
                evidence: Vec::new(),
            },
        );
        serde_json::to_value(CuratedManifest {
            meta,
            manifest_kind: ManifestKindId::new("synthetic.kind"),
            body: json!({"label": label}),
        })
        .unwrap()
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
            let schema = payload_schema(&world, f.kind).unwrap();
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
            let checked = check_candidate(
                &World::new(),
                CandidateCheck {
                    asset: asset(f.kind, f.id, captured["payload"].clone()),
                    driver_cases: vec![],
                    catalog_queries: vec![],
                },
            )
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
    fn declared_table_priors_are_portable_and_native_priors_carry_their_gap() {
        let provenance = json!({"source":"synthetic","source_version":"v","jurisdiction":null,
            "ingested_at":0,"license":{"kind":"Cc0"},"backlink":null,"supersedes":[]});
        let spec: GenerationPriorSpec = from(
            json!({"id":"synthetic.table","label":"table","description":"d",
            "scope":{"kind":"RecipeSelection","element_class":"c","recipe_family":null},
            "source_provenance":provenance.clone(),"cases":[],
            "otherwise":{"weight":0.5,"suggestion":null,"rationale":"default"}}),
        );
        let mut app = App::new();
        app.init_resource::<CapabilityRegistry>();
        app.register_declarative_generation_prior(spec.clone());
        app.register_native_generation_prior(
            crate::capability_registry::GenerationPriorDescriptor {
                id: PriorId("synthetic.native".into()),
                label: "native".into(),
                description: "d".into(),
                scope: from(
                    json!({"kind":"RecipeSelection","element_class":"c","recipe_family":null}),
                ),
                source_provenance: from(provenance),
                prior_fn: std::sync::Arc::new(|_| {
                    crate::capability_registry::PriorEvaluation::neutral("native")
                }),
            },
            "synthetic native reason",
        );
        let world = app.world();
        assert_eq!(
            listed(world, "generation_prior.v1", "synthetic.table").unwrap()["portable"],
            true
        );
        let exact = legacy_get(
            world,
            LegacyRequest {
                kind: "generation_prior.v1".into(),
                id: "synthetic.table".into(),
            },
        )
        .unwrap();
        assert_eq!(exact["payload"], serde_json::to_value(&spec).unwrap());
        let native = listed(world, "generation_prior.v1", "synthetic.native").unwrap();
        assert_eq!(native["portable"], false);
        assert_eq!(native["gap"], "synthetic native reason");
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
        assert!(payload_schema(&World::new(), "generation_prior.v1").is_ok());

        let spec = json!({"id":"synthetic.prior","label":"corpus","description":"d",
            "scope":{"kind":"RecipeSelection","element_class":"c","recipe_family":null},
            "source_provenance":provenance,
            "cases":[{"when":{"jurisdiction":"se","terrain_slope_pct_min":10.0},
                      "then":{"weight":0.25,"suggestion":null,"rationale":"steep"}}],
            "otherwise":{"weight":1.0,"suggestion":null,"rationale":"flat"}});
        check_candidate(
            &World::new(),
            CandidateCheck {
                asset: asset("generation_prior.v1", "synthetic.prior", spec.clone()),
                driver_cases: vec![],
                catalog_queries: vec![],
            },
        )
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

    // ----- plugin-registered kinds -------------------------------------------------

    #[derive(Resource, Default)]
    struct PluginStore(BTreeMap<String, i64>);

    #[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
    struct PluginPayload {
        id: String,
        value: i64,
    }

    /// A kind core cannot name, as a domain plugin would declare it.
    struct PluginKind;
    impl RegistryKind for PluginKind {
        type Payload = PluginPayload;
        const KIND: &'static str = "synthetic_plugin.v1";
        const CONSTRAINTS: &'static [&'static str] = &["payload.id must equal asset.id"];
        fn payload_id(p: &Self::Payload) -> &str {
            &p.id
        }
        fn install(world: &mut World, p: Self::Payload) {
            world.init_resource::<PluginStore>();
            world.resource_mut::<PluginStore>().0.insert(p.id, p.value);
        }
        fn withdraw(world: &mut World, p: &Self::Payload) {
            if let Some(mut store) = world.get_resource_mut::<PluginStore>() {
                store.0.remove(&p.id);
            }
        }
        fn legacy(world: &World) -> Vec<Legacy<Self::Payload>> {
            world
                .get_resource::<PluginStore>()
                .map(|s| {
                    s.0.iter()
                        .map(|(id, value)| Legacy {
                            id: id.clone(),
                            label: id.clone(),
                            payload: Ok(PluginPayload {
                                id: id.clone(),
                                value: *value,
                            }),
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
    }

    fn plugin_asset(value: i64) -> Asset {
        let mut a = asset(
            "relation_type.v1",
            "synthetic.plugin",
            json!({"id":"synthetic.plugin","value":value}),
        );
        a.kind = PluginKind::KIND.into();
        a.path_kind = "synthetic_plugin".into();
        a
    }

    #[test]
    fn plugin_registered_kind_routes_through_every_path() {
        use super::super::{check_payload, install_payload};
        let mut app = App::new();
        app.register_knowledge_kind::<PluginKind>();
        let world = app.world_mut();
        let plain = World::new();

        // Schema and routing exist only where the plugin registered the kind.
        let schema = payload_schema(world, PluginKind::KIND).unwrap();
        assert_eq!(
            schema["constraints"],
            json!(["payload.id must equal asset.id"])
        );
        assert!(schema["payload_schema"]["properties"]["value"].is_object());
        assert!(payload_schema(&plain, PluginKind::KIND).is_err());
        // Core kinds stay routed alongside it.
        assert!(payload_schema(world, "material.v1").is_ok());

        // Candidate check runs in a disposable world and leaves the live one untouched.
        let request = |value| CandidateCheck {
            asset: plugin_asset(value),
            driver_cases: vec![],
            catalog_queries: vec![],
        };
        check_payload(world, request(1)).unwrap();
        assert!(world.get_resource::<PluginStore>().is_none());

        // Install, replace, and the legacy inventory.
        install_payload(world, admitted(plugin_asset(1), "r1")).unwrap();
        assert_eq!(world.resource::<PluginStore>().0["synthetic.plugin"], 1);
        install_payload(world, admitted(plugin_asset(2), "r2")).unwrap();
        assert_eq!(world.resource::<PluginStore>().0["synthetic.plugin"], 2);
        assert!(listed(world, PluginKind::KIND, "synthetic.plugin").is_none());
        assert!(legacy_list(world)["coverage"]["portable_kinds"]
            .as_array()
            .unwrap()
            .contains(&json!(PluginKind::KIND)));
        assert!(!legacy_list(&plain)["coverage"]["portable_kinds"]
            .as_array()
            .unwrap()
            .contains(&json!(PluginKind::KIND)));

        // Withdrawal reaches the plugin handler and the entry becomes a legacy candidate again.
        withdrawn(world, "synthetic.plugin");
        assert!(world.resource::<PluginStore>().0.is_empty());
        world
            .resource_mut::<PluginStore>()
            .0
            .insert("compiled.entry".into(), 7);
        assert_eq!(
            listed(world, PluginKind::KIND, "compiled.entry").unwrap()["portable"],
            true
        );
        let captured = legacy_get(
            world,
            LegacyRequest {
                kind: PluginKind::KIND.into(),
                id: "compiled.entry".into(),
            },
        )
        .unwrap();
        assert_eq!(
            captured["payload"],
            json!({"id":"compiled.entry","value":7})
        );
    }

    #[test]
    fn registering_an_existing_kind_replaces_its_handler() {
        let mut registry = KnowledgeKindRegistry::default();
        let count = registry.iter().count();
        registry.register(KindHandler::of::<Material>());
        assert_eq!(registry.iter().count(), count);
        assert!(registry.get("material.v1").is_some());
    }

    // ----- material.v1 ---------------------------------------------------------------

    #[test]
    fn textured_materials_are_refused_and_compiled_ones_are_gaps() {
        let mut world = World::new();
        let mut textured = MaterialDef {
            id: "synthetic.textured".into(),
            ..MaterialDef::default()
        };
        textured.base_color_texture = Some(crate::plugins::materials::TextureRef::AssetPath {
            path: "a/b.png".into(),
        });
        let payload = serde_json::to_value(&textured).unwrap();
        let error = super::super::install(
            &mut world,
            admitted(asset("material.v1", "synthetic.textured", payload), "r"),
        )
        .unwrap_err();
        assert!(error.contains("not portable"), "{error}");

        world.init_resource::<BuiltinMaterialDefaults>();
        world
            .resource_mut::<BuiltinMaterialDefaults>()
            .insert(textured);
        let item = listed(&world, "material.v1", "synthetic.textured").unwrap();
        assert_eq!(item["portable"], false);
        assert!(item["gap"].as_str().unwrap().contains("texture"));
        // Compiled procedural builtins are portable candidates.
        assert!(legacy_list(&world)["assets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["kind"] == "material.v1" && a["portable"] == true));
    }

    #[test]
    fn installed_material_reaches_the_live_registry_and_survives_reseed() {
        let mut world = World::new();
        world.insert_resource(MaterialRegistry::default());
        let payload = serde_json::to_value(MaterialDef {
            id: "synthetic.live".into(),
            name: "corpus".into(),
            ..MaterialDef::default()
        })
        .unwrap();
        install_asset(&mut world, "material.v1", "synthetic.live", payload, "m1");
        assert!(world
            .resource::<MaterialRegistry>()
            .contains("synthetic.live"));
        world.insert_resource(MaterialRegistry::default());
        crate::plugins::materials::reseed_builtin_materials(&mut world);
        assert!(world
            .resource::<MaterialRegistry>()
            .contains("synthetic.live"));
        withdrawn(&mut world, "synthetic.live");
        assert!(!world
            .resource::<MaterialRegistry>()
            .contains("synthetic.live"));
    }

    // ----- constraint_rule.v1 ----------------------------------------------------------

    #[test]
    fn compiled_constraints_are_native_gaps_unless_declared_as_rules() {
        use crate::capability_registry::{
            Applicability, ConstraintDescriptor, ConstraintRole, Severity,
        };
        let mut world = World::new();
        registry(&mut world).register_constraint(ConstraintDescriptor {
            id: ConstraintId("synthetic.native".into()),
            label: "native".into(),
            description: "d".into(),
            applicability: Applicability::any(),
            default_severity: Severity::Advice,
            rationale: "r".into(),
            source_backlink: None,
            role: ConstraintRole::Validation,
            validator: std::sync::Arc::new(|_, _| Vec::new()),
        });
        install_rule(&mut world, from(rule_json("declared")));
        let native = listed(&world, "constraint_rule.v1", "synthetic.native").unwrap();
        assert_eq!(native["portable"], false);
        assert!(native["gap"].as_str().unwrap().contains("closure"));
        assert!(legacy_get(
            &world,
            LegacyRequest {
                kind: "constraint_rule.v1".into(),
                id: "synthetic.native".into()
            }
        )
        .is_err());
        assert_eq!(
            listed(&world, "constraint_rule.v1", "synthetic.rule").unwrap()["portable"],
            true
        );
    }

    #[test]
    fn invalid_rules_are_refused() {
        for (field, bad) in [
            ("parameter_path", json!("height")),
            ("citation", json!(" ")),
            (
                "requirement",
                json!({"comparator":"range","min":3.0,"max":1.0}),
            ),
            ("requirement", json!({"comparator":"gt","value":null})),
        ] {
            let mut payload = rule_json("x");
            payload[field] = bad;
            let mut world = World::new();
            assert!(
                super::super::install(
                    &mut world,
                    admitted(asset("constraint_rule.v1", "synthetic.rule", payload), "r")
                )
                .is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn installed_rule_produces_findings_through_the_validation_sweep_until_withdrawn() {
        use crate::capability_registry::{ElementClassAssignment, Severity};
        use crate::plugins::{
            identity::ElementId,
            refinement::{RefinementState, RefinementStateComponent, SemanticIntent},
            validation::{validation_sweep_system, Findings},
        };
        let mut world = World::new();
        world.init_resource::<CapabilityRegistry>();
        let spawn = |world: &mut World, id: u64, state: RefinementState, params: Value| {
            world
                .spawn((
                    ElementId(id),
                    ElementClassAssignment {
                        element_class: ElementClassId("synthetic.class".into()),
                        active_recipe: None,
                    },
                    RefinementStateComponent { state },
                    SemanticIntent {
                        parameters: params,
                        ..Default::default()
                    },
                ))
                .id()
        };
        let below = spawn(
            &mut world,
            1,
            RefinementState::Schematic,
            json!({"height":1.5}),
        );
        spawn(
            &mut world,
            2,
            RefinementState::Schematic,
            json!({"height":2.5}),
        );
        spawn(
            &mut world,
            3,
            RefinementState::Conceptual,
            json!({"height":1.5}),
        );
        spawn(
            &mut world,
            4,
            RefinementState::Schematic,
            json!({"other":1.5}),
        );
        let _ = below;

        let sweep = |world: &mut World| {
            validation_sweep_system(world);
            world
                .resource::<Findings>()
                .all()
                .map(|f| {
                    (
                        f.subject,
                        f.constraint_id.0.clone(),
                        f.severity,
                        f.backlink.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert!(sweep(&mut world).is_empty());

        install_asset(
            &mut world,
            "constraint_rule.v1",
            "synthetic.rule",
            rule_json("r"),
            "c1",
        );
        let findings = sweep(&mut world);
        assert_eq!(findings.len(), 1, "{findings:?}");
        let (subject, constraint, severity, backlink) = &findings[0];
        assert_eq!((*subject, constraint.as_str()), (1, "synthetic.rule"));
        assert_eq!(*severity, Severity::Warning);
        assert_eq!(
            backlink.as_ref().map(|p| p.0.as_str()),
            Some("synthetic/passage/1")
        );
        assert!(world
            .resource::<CapabilityRegistry>()
            .constraint_descriptor(&ConstraintId("synthetic.rule".into()))
            .is_some());

        // A revision that tightens the requirement replaces the validator in place.
        let mut stricter = rule_json("r");
        stricter["requirement"] = json!({"comparator":"gte","value":3.0});
        install_asset(
            &mut world,
            "constraint_rule.v1",
            "synthetic.rule",
            stricter,
            "c2",
        );
        assert_eq!(sweep(&mut world).len(), 2);
        assert_eq!(
            world
                .resource::<CapabilityRegistry>()
                .constraint_descriptors()
                .len(),
            1
        );

        withdrawn(&mut world, "synthetic.rule");
        assert!(sweep(&mut world).is_empty());
        assert!(world
            .resource::<CapabilityRegistry>()
            .constraint_descriptor(&ConstraintId("synthetic.rule".into()))
            .is_none());
    }

    #[test]
    fn jurisdiction_scoped_rule_only_checks_matching_subjects() {
        use crate::capability_registry::ElementClassAssignment;
        use crate::plugins::{
            identity::ElementId,
            refinement::{RefinementState, RefinementStateComponent, SemanticIntent},
            validation::{validation_sweep_system, Findings},
        };
        let mut world = World::new();
        world.init_resource::<CapabilityRegistry>();
        for (id, j) in [(1, "SE"), (2, "us")] {
            world.spawn((
                ElementId(id),
                ElementClassAssignment {
                    element_class: ElementClassId("synthetic.class".into()),
                    active_recipe: None,
                },
                RefinementStateComponent {
                    state: RefinementState::Detailed,
                },
                SemanticIntent {
                    parameters: json!({"height":1.0,"jurisdiction":j}),
                    ..Default::default()
                },
            ));
        }
        let mut rule = rule_json("r");
        rule["jurisdiction"] = json!("se");
        install_asset(
            &mut world,
            "constraint_rule.v1",
            "synthetic.rule",
            rule,
            "j1",
        );
        validation_sweep_system(&mut world);
        let subjects: Vec<u64> = world
            .resource::<Findings>()
            .all()
            .map(|f| f.subject)
            .collect();
        assert_eq!(subjects, vec![1]);
    }

    // ----- setting_out_prior.v1 / curated_manifest.v1 ------------------------------------

    #[test]
    fn invalid_setting_out_priors_and_manifests_are_refused() {
        let mut world = World::new();
        let prior = |edit: fn(&mut Value)| {
            let mut v = json!({"prior_id":"synthetic.p","element_class":"c","construction_system_id":null,
                "jurisdiction":null,"thickness_mm":100.0,"negative_offset_mm":50.0,
                "positive_offset_mm":50.0,"source_ref":"s","confidence":0.5});
            edit(&mut v);
            v
        };
        for edit in [
            (|v: &mut Value| v["source_ref"] = json!(" ")) as fn(&mut Value),
            |v| v["confidence"] = json!(1.5),
            |v| v["positive_offset_mm"] = json!(10.0),
        ] {
            assert!(super::super::install(
                &mut world,
                admitted(
                    asset("setting_out_prior.v1", "synthetic.p", prior(edit)),
                    "r"
                )
            )
            .is_err());
        }
        let mut manifest = manifest_json("x");
        manifest["body"] = json!([1]);
        assert!(super::super::install(
            &mut world,
            admitted(
                asset("curated_manifest.v1", "synthetic.kind/m1", manifest),
                "r"
            )
        )
        .is_err());
        let mut wrong_kind = manifest_json("x");
        wrong_kind["meta"]["kind"] = json!("recipe.v1");
        assert!(super::super::install(
            &mut world,
            admitted(
                asset("curated_manifest.v1", "synthetic.kind/m1", wrong_kind),
                "r"
            )
        )
        .is_err());
    }
}
