//! Explicit host/planner declarations, not dependencies inferred from agent text.
//! Admission reads authenticated last-known state only; it performs no I/O.
use super::{PlanContext, Refusal};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_HARNESS_DEPENDENCIES: usize = 32;
const MAX_PLANNER_BINDINGS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessDependency {
    pub authority_digest: String,
    pub artifact_id: String,
    pub revision: String,
}

fn token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b))
}

pub(crate) fn normalize(dependencies: &mut Vec<HarnessDependency>) -> Result<(), String> {
    if dependencies.len() > MAX_HARNESS_DEPENDENCIES {
        return Err("Too many declared harness dependencies".into());
    }
    for dependency in dependencies.iter() {
        if dependency.authority_digest.len() != 64
            || !dependency
                .authority_digest
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || !token(&dependency.artifact_id)
            || !dependency.artifact_id.contains(':')
            || !token(&dependency.revision)
        {
            return Err("Invalid bounded harness dependency identity".into());
        }
    }
    dependencies.sort();
    dependencies.dedup();
    if dependencies.windows(2).any(|pair| {
        pair[0].authority_digest == pair[1].authority_digest
            && pair[0].artifact_id == pair[1].artifact_id
            && pair[0].revision != pair[1].revision
    }) {
        return Err("Conflicting revisions for one declared harness dependency".into());
    }
    Ok(())
}

/// Installed only by trusted host/capability code for the actual planner build.
/// No MCP request configures this resource. A release does not install a binding.
#[derive(Resource, Default)]
pub struct HarnessPlannerBindings {
    entries: BTreeMap<(String, u32), Vec<HarnessDependency>>,
}

impl HarnessPlannerBindings {
    pub fn bind(
        &mut self,
        planner_id: &str,
        planner_version: u32,
        mut dependencies: Vec<HarnessDependency>,
    ) -> Result<(), String> {
        if !token(planner_id) {
            return Err("Invalid planner identity".into());
        }
        normalize(&mut dependencies)?;
        let key = (planner_id.to_owned(), planner_version);
        if !self.entries.contains_key(&key) && self.entries.len() >= MAX_PLANNER_BINDINGS {
            return Err("Harness planner binding capacity reached".into());
        }
        self.entries.insert(key, dependencies);
        Ok(())
    }

    fn get(&self, context: &PlanContext) -> &[HarnessDependency] {
        self.entries
            .get(&(context.planner_id.clone(), context.planner_version))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

pub(crate) fn capture_context(world: &World, context: &mut PlanContext) -> Result<(), String> {
    // Bound caller input before inspecting it; host entries were bounded at bind.
    normalize(&mut context.harness_dependencies)?;
    if let Some(bindings) = world.get_resource::<HarnessPlannerBindings>() {
        let declared = bindings.get(context).to_vec();
        context.harness_dependencies.extend(declared);
        // The temporary merge is at most 64 entries; the final unique set is 32.
        context.harness_dependencies.sort();
        context.harness_dependencies.dedup();
    }
    normalize(&mut context.harness_dependencies)
}

pub(crate) fn has_declared_dependencies(world: &World, context: &PlanContext) -> bool {
    !context.harness_dependencies.is_empty()
        || world
            .get_resource::<HarnessPlannerBindings>()
            .is_some_and(|bindings| !bindings.get(context).is_empty())
}

pub(crate) fn admission(world: &World, dependencies: &[HarnessDependency]) -> Option<Refusal> {
    if dependencies.is_empty() {
        return None;
    }
    #[cfg(feature = "model-api")]
    let result = world
        .get_resource::<crate::plugins::curation::CurationReleaseStore>()
        .ok_or_else(|| "No curation verifier is installed for this declared dependency".to_owned())
        .and_then(|store| store.check_dependencies(dependencies));
    #[cfg(not(feature = "model-api"))]
    let result: Result<(), String> = {
        let _ = world;
        Err("This build has no curation verifier for declared harness dependencies".into())
    };
    result.err().map(|reason| Refusal {
        violated: None,
        reason,
        observed: "Captured host-declared harness dependencies; checked against authenticated last-known state, not current network freshness".into(),
        repair: "Refresh curation status, ask the host to install a compatible reviewed planner binding, and create a new preview. Undo remains available; unrelated undeclared plans are unaffected.".into(),
        contrasts: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reference(index: usize) -> HarnessDependency {
        HarnessDependency {
            authority_digest: "a".repeat(64),
            artifact_id: format!("skill:test.{index}"),
            revision: "r1".into(),
        }
    }
    #[test]
    fn host_binding_capture_is_idempotent_at_capacity_and_rejects_raw_oversize() {
        let mut world = World::new();
        let references: Vec<_> = (0..MAX_HARNESS_DEPENDENCIES).map(reference).collect();
        let mut bindings = HarnessPlannerBindings::default();
        bindings.bind("test", 1, references.clone()).unwrap();
        world.insert_resource(bindings);
        let mut context = PlanContext {
            planner_id: "test".into(),
            planner_version: 1,
            harness_dependencies: references.clone(),
            ..Default::default()
        };
        capture_context(&world, &mut context).unwrap();
        capture_context(&world, &mut context).unwrap();
        assert_eq!(
            context.harness_dependencies,
            references
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        );
        context.harness_dependencies.push(reference(0));
        assert!(capture_context(&world, &mut context).is_err());
        let mut conflicting = vec![reference(0), reference(0)];
        conflicting[1].revision = "r2".into();
        assert!(normalize(&mut conflicting).is_err());
        assert!(admission(&World::new(), &[reference(0)]).is_some());
        assert!(admission(&World::new(), &[]).is_none());
    }
}
